#!/usr/bin/env python3
"""Verify only the isolated passive native-append experiment, never admission."""
import argparse
import hashlib
import json
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument('root', type=Path)
args = parser.parse_args()
root = args.root.resolve(strict=True)
manifest = json.loads((root / 'manifest.json').read_text())
events = [json.loads(line) for line in (root / 'events.jsonl').read_text().splitlines()]
session = manifest['session']
starts = [event['data'] for event in events if event['kind'] == 'append_session']
assert len(starts) == 1 and starts[0]['root'] == session
assert starts[0]['version']['version'] == starts[0]['version']['base'] == '2.1.302'
classic = [event['data'] for event in events if event['kind'] == 'append_classic_session']
assert len(classic) == 1 and classic[0]['session_id'] == session
results = [event for event in events if event['kind'] == 'append_result']
assert [event['data']['sequence'] for event in results] == [1, 2, 3]
assert all(event['data']['exact'] and event['data']['root'] == session for event in results)
assert all(before['time'] < after['time'] for before, after in zip(results, results[1:]))
uuids = [event['data']['result']['uuid'] for event in results]
assert len(set(uuids)) == 3
frames = []
for event in results:
    data = event['data']
    payload = json.loads((root / 'payloads' / f'{data["sequence"]:020d}.json').read_text())
    expected = ('AGENTCHAT_APPEND_PROBE full native input, passive context only.\n'
                + json.dumps(payload, ensure_ascii=False, separators=(',', ':')))
    message = data['result']['message']
    assert message['type'] == message['role'] == 'user' and message['isMeta'] is True
    assert message['content'] == [{'type': 'text', 'text': expected}]
    frames.append({'sequence': data['sequence'], 'uuid': data['result']['uuid'],
                   'body_bytes': len(payload['body'].encode()),
                   'body_sha256': hashlib.sha256(payload['body'].encode()).hexdigest(),
                   'native_frame_sha256': hashlib.sha256(expected.encode()).hexdigest(),
                   'native_acceptance_time': event['time']})
assert [frame['body_bytes'] for frame in frames] == [37, 65_536, 36]
idle = json.loads((root / 'idle-observation.json').read_text())
assert idle['idle_seconds_since_acceptance'] >= 20
assert idle['model_requests'] == idle['baseline_model_requests'] == 1
assert idle['new_prompt_submits'] == 0
assert idle['accepted_sequences'] == [1, 2, 3]
files = [event for event in events if event['kind'] == 'append_file']
assert len(files) >= 2, 'Repeat file signal was not observed'
assert files[1]['time'] > results[-1]['time']
assert len([event for event in events if event['kind'] == 'append_before']) == 3
assert not any(event['kind'] == 'hook_stderr_before_exit2' for event in events)
completions = [event for event in events if event['kind'] == 'append_complete']
assert len(completions) == 2
assert completions[1]['time'] > idle['observed_at']
assert completions[1]['data']['root'] == session
assert completions[1]['data']['event']['isAborted'] is False
answer = completions[1]['data']['event']['answer']
codes = ['CODE_AMBER_17', 'CODE_RUBY_62', 'CODE_JADE_93']
assert all(code in answer for code in codes)
assert [answer.index(code) for code in codes] == sorted(answer.index(code) for code in codes)
# Inspect only this owned conversation's native transcript, never credentials.
project = str(root / 'workspace').replace('/', '-')
transcript = Path(manifest['profile']) / 'projects' / project / f'{session}.jsonl'
rows = [json.loads(line) for line in transcript.read_text().splitlines()]
stored = [row for row in rows if row.get('uuid') in uuids]
assert [row['uuid'] for row in stored] == uuids
for row, event in zip(stored, results):
    assert row['sessionId'] == session and row['type'] == 'user'
    assert row['message']['content'] == event['data']['result']['message']['content']
(root / 'owned-transcript.jsonl').write_text(
    ''.join(json.dumps(row, ensure_ascii=False) + '\n' for row in stored))
report = {'scope': 'Manual native append, not product FIFO or profile admission',
          'session': session, 'version': starts[0]['version'], 'frames': frames,
          'idle': idle, 'repeat_file_signals': len(files), 'followup_answer': answer,
          'transcript_rows_match': True, 'application_cursor_changed': False}
(root / 'append-verification.json').write_text(json.dumps(report, indent=2) + '\n')
print(json.dumps(report, indent=2))

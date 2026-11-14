#!/usr/bin/env python3
"""Verify selected native observations; never promote an incomplete control gate."""
import hashlib
import json
from pathlib import Path
import sys

root = Path(sys.argv[1]).resolve(strict=True)
manifest = json.loads((root / 'manifest.json').read_text())
events = [json.loads(line) for line in (root / 'events.jsonl').read_text().splitlines()]
driver = [json.loads(line) for line in (root / 'driver-events.jsonl').read_text().splitlines()]
payloads = {sequence: json.loads((root / 'payloads' / f'{sequence:020d}.json').read_text())
            for sequence in (1, 2, 3, 4)}
starts = [entry for entry in events if entry['kind'] == 'session_start_after']
assert len(starts) == 1
assert starts[0]['data']['root'] == manifest['session']
assert starts[0]['data']['version']['version'] == '2.1.302'
assert starts[0]['data']['aborted'] is False
handoffs = []
receipts = []
for sequence in (1, 2, 3):
    payload = payloads[sequence]
    exact = json.dumps(payload, ensure_ascii=False)
    matches = [entry for entry in events if entry['kind'] == 'prompt_after' and
               exact in entry['data'].get('result', {}).get('text', '')]
    assert len(matches) == 1
    accepted = matches[0]
    assert accepted['data']['root'] == manifest['session']
    assert accepted['data']['aborted'] is False
    assert accepted['data']['paused'] is False and accepted['data']['approval'] is False
    assert accepted['data']['result']['origin'] == {'kind': 'task-notification'}
    expected = payload['body'].split('Reply exactly ')[1].split('.')[0]
    models = [entry for entry in events if entry['kind'] == 'turn_complete' and
              entry['data']['event'].get('answer') == expected]
    assert len(models) == 1 and models[0]['time'] > accepted['time']
    assert models[0]['data']['event']['isAborted'] is False
    receipts.append(models[0]['time'])
    handoffs.append({'sequence': sequence, 'message_id': payload['message_id'],
                     'body_sha256': hashlib.sha256(payload['body'].encode()).hexdigest(),
                     'native_prompt_result_time': accepted['time'],
                     'model_answer_time': models[0]['time']})
assert [entry['native_prompt_result_time'] for entry in handoffs] == sorted(
    entry['native_prompt_result_time'] for entry in handoffs)
failed = [entry for entry in events if entry['kind'] == 'hook_failed_before_handoff']
assert len(failed) == 2
reads = [entry for entry in events if entry['kind'] == 'hook_read']
assert all(entry['data']['payload'] == payloads[1] for entry in reads[:3])
assert failed[-1]['time'] < handoffs[0]['native_prompt_result_time']
assert all(entry['time'] < handoffs[0]['native_prompt_result_time'] for entry in failed)
assert not any(entry['kind'] == 'tool_after' and
               handoffs[0]['native_prompt_result_time'] <= entry['time'] <= receipts[-1]
               for entry in events)
first_file_result = next(entry for entry in events if entry['kind'] == 'file_after')
assert first_file_result['time'] < reads[0]['time'] < failed[0]['time']
assert not any(entry['kind'] == 'prompt_after' and
               'seq=4 ' in entry['data'].get('result', {}).get('text', '') for entry in events)
assert not (root / 'workspace/approval-fence.txt').exists()
hold4 = next(entry for entry in reads if entry['data']['payload']['event_seq'] == 4)
abandoned = next(entry for entry in events if entry['kind'] == 'hook_hold_abandoned')
released = next(entry for entry in driver if entry['kind'] == 'release_head4_during_native_approval')
assert hold4['time'] < abandoned['time'] < released['time']
restored = json.loads((root / 'restoration.json').read_text())
assert restored['restored_exact'] and restored['stowed_restored']
assert restored['native']['recipient']['exit'] == 0
report = {'scope': 'Selected manual short-body path only; not product FIFO or G0',
          'version': '2.1.302', 'session': manifest['session'],
          'handoffs': handoffs, 'failure_replays': len(failed),
          'first_hook_delay_seconds': failed[0]['time'] - reads[0]['time'],
          'file_callback_returned_before_hook_read': True,
          'no_body_fetch_tool_in_three_body_window': True,
          'control_driver': 'FAILED: Native Esc did not produce an observed abort',
          'held_approval_case': 'EXCLUDED: hook expired before release',
          'body4': 'Retained; no observed native acceptance; not an application backlog test',
          'remaining': ['queued acceptance versus model-context consumption decision',
                        'in-flight approval/Esc, clear/fork and original-owner loss composition',
                        'maximum-size and escaping bodies', 'production FIFO and Linux'],
          'restoration': restored}
(root / 'verification.json').write_text(json.dumps(report, indent=2) + '\n')
print(json.dumps(report, indent=2))

#!/usr/bin/env python3
"""Restore only the dedicated profile after all fixture-native owners terminate."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import urllib.request

parser = argparse.ArgumentParser()
parser.add_argument('root', type=Path)
parser.add_argument('--reason', required=True)
args = parser.parse_args()
root = args.root.resolve(strict=True)
manifest = json.loads((root / 'manifest.json').read_text())
profile = Path(manifest['profile'])
assert str(profile).startswith(('/private/var/tmp/', '/var/tmp/'))
server = json.loads((root / 'terminal-control.json').read_text())
request = urllib.request.Request(
    f'http://127.0.0.1:{server["port"]}', data=b'{"op":"list"}',
    headers={'Content-Type': 'application/json'},
)
with urllib.request.urlopen(request, timeout=5) as response:
    terms = json.load(response)
assert all(term['exit'] is not None for term in terms.values()), 'Native owner still running'


def changed_paths(before, after, prefix=''):
    if isinstance(before, dict) and isinstance(after, dict):
        for key in sorted(set(before) | set(after)):
            yield from changed_paths(before.get(key), after.get(key), prefix + '/' + key)
    elif before != after:
        yield prefix


prepared = root / 'prepared'
if prepared.exists():
    settings = (prepared / 'settings.json').read_bytes()
    before = json.loads((prepared / '.claude.json').read_bytes())
else:
    settings = (root / 'preimage/settings.json').read_bytes()
    before = json.loads((root / 'preimage/.claude.json').read_bytes())
    before.get('mcpServers', {}).pop('agentchat', None)
assert (profile / 'settings.json').read_bytes() == settings, 'Unexpected settings edit'
after = json.loads((profile / '.claude.json').read_bytes())
changed = list(changed_paths(before, after))
allowed = ['/cachedGrowthBookFeaturesAt', '/numStartups',
           '/promptQueueUseCount',
           '/tipLifetimeShownCounts', '/tipsHistory',
           '/pluginUsage/agentchat-body-boundary-probe@inline', '/projects/' + str(root / 'workspace')]
assert all(any(path == value or path.startswith(value + '/') for value in allowed)
           for path in changed), changed
post = root / 'postimage'
post.mkdir()
for name, expected in manifest['preimages'].items():
    (post / name).write_bytes((profile / name).read_bytes())
    original = (root / 'preimage' / name).read_bytes()
    assert hashlib.sha256(original).hexdigest() == expected
    (profile / name).write_bytes(original)
for name in manifest['stowed']:
    destination = profile / name
    assert not destination.exists()
    (root / 'stowed-agentchat-native-test').rename(destination)
command = subprocess.run(['ps', '-o', 'command=', '-p', str(server['pid'])],
                         capture_output=True, text=True, check=True).stdout
assert str(root / 'terminal.py') in command, 'Controller identity changed'
os.kill(server['pid'], signal.SIGTERM)
report = {'native': {name: {'pid': term['pid'], 'exit': term['exit']}
                     for name, term in terms.items()},
          'changed_host_paths': changed, 'reason': args.reason,
          'restored_exact': all(hashlib.sha256((profile / name).read_bytes()).hexdigest() == value
                                for name, value in manifest['preimages'].items()),
          'stowed_restored': all((profile / name).exists() for name in manifest['stowed']),
          'controller_sigterm': server['pid']}
(root / 'restoration.json').write_text(json.dumps(report, indent=2) + '\n')
print(json.dumps(report, indent=2))

#!/usr/bin/env python3
"""Operate only the bounded PTY controller created for this explicit fixture."""
import argparse
import json
from pathlib import Path
import subprocess
import urllib.request

parser = argparse.ArgumentParser()
parser.add_argument('root', type=Path)
parser.add_argument('operation', choices=['start', 'screen', 'input', 'list', 'signal', 'identity'])
parser.add_argument('--name', default='recipient')
parser.add_argument('--text')
parser.add_argument('--signal', type=int)
args = parser.parse_args()
root = args.root.resolve(strict=True)
manifest = json.loads((root / 'manifest.json').read_text())
server = json.loads((root / 'terminal-control.json').read_text())
request = {'op': args.operation, 'name': args.name}
if args.operation == 'start':
    request.update(argv=[manifest['native'], '--session-id', manifest['session'],
                         '--plugin-dir', str(root / 'plugin')],
                   cwd=str(root / 'workspace'),
                   env={'CLAUDE_CONFIG_DIR': manifest['profile'], 'AGENTCHAT_HOME': str(root / 'agentchat-home'),
                        'AGENTCHAT_USER_CONFIG_ROOT': str(root / 'user-config'), 'DISABLE_AUTOUPDATER': '1'})
elif args.operation == 'input':
    request['text'] = args.text
elif args.operation == 'signal':
    request['signal'] = args.signal
elif args.operation == 'identity':
    request['op'] = 'screen'
data = json.dumps(request).encode()
with urllib.request.urlopen(urllib.request.Request(
        f'http://127.0.0.1:{server["port"]}', data=data, headers={'Content-Type': 'application/json'}), timeout=10) as response:
    result = json.load(response)
if args.operation == 'identity':
    if (root / 'owner.json').exists():
        raise SystemExit('Original owner identity is immutable')
    identity = subprocess.run([manifest['identity_helper'], 'identity', str(result['pid'])],
                              capture_output=True, timeout=5, check=True)
    owner = json.loads(identity.stdout)
    (root / 'owner.json').write_text(json.dumps(owner, indent=2) + '\n')
    result = owner
print(json.dumps(result, ensure_ascii=False, indent=2))

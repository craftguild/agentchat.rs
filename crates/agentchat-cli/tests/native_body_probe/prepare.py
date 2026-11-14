#!/usr/bin/env python3
"""Prepare an explicit disposable real-host fixture; never copy credentials."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import shlex
import sys
import uuid

parser = argparse.ArgumentParser()
parser.add_argument('root', type=Path)
parser.add_argument('--profile', type=Path, required=True)
parser.add_argument('--native', type=Path, required=True)
parser.add_argument('--identity-helper', type=Path, required=True)
parser.add_argument('--module', type=Path)
args = parser.parse_args()
root = args.root.resolve()
profile = args.profile.resolve(strict=True)
native = args.native.resolve(strict=True)
helper = args.identity_helper.resolve(strict=True)
if not str(profile).startswith(('/private/var/tmp/', '/var/tmp/')) or profile.name != 'host-config':
    raise SystemExit('An explicit retained disposable profile is required')
root.mkdir(mode=0o700)
source = Path(__file__).resolve().parent
for name in ('hook.py', 'terminal.py'):
    shutil.copyfile(source / name, root / name)
for name in ('workspace', 'payloads', 'plugin', 'preimage', 'prepared', 'agentchat-home', 'user-config'):
    (root / name).mkdir(mode=0o700)
session = str(uuid.uuid4())
manifest = {'version': 1, 'session': session, 'root': str(root), 'profile': str(profile),
            'native': str(native), 'identity_helper': str(helper), 'python': sys.executable,
            'preimages': {}, 'stowed': []}
for name in ('settings.json', '.claude.json'):
    path = profile / name
    if path.exists():
        data = path.read_bytes()
        (root / 'preimage' / name).write_bytes(data)
        manifest['preimages'][name] = hashlib.sha256(data).hexdigest()
# These are the retained fixture's owned integration, not user-wide real config.
integration = profile / 'skills/agentchat-native-test'
if integration.exists():
    integration.rename(root / 'stowed-agentchat-native-test')
    manifest['stowed'].append('skills/agentchat-native-test')
config = profile / '.claude.json'
if config.exists():
    value = json.loads(config.read_bytes())
    value.get('mcpServers', {}).pop('agentchat', None)
    config.write_text(json.dumps(value, indent=2) + '\n')
manifest['prepared_config_sha256'] = hashlib.sha256(config.read_bytes()).hexdigest()
settings_path = profile / 'settings.json'
settings = json.loads(settings_path.read_text()) if settings_path.exists() else {}
settings.setdefault('hooks', {}).setdefault('FileChanged', []).append({
    'matcher': '.agentchat-body-probe-wake', 'hooks': [{
        'type': 'command',
        'command': shlex.join([sys.executable, str(root / 'hook.py'), str(root), 'FileChanged']),
        'asyncRewake': True, 'timeout': 50,
    }],
})
settings_path.write_text(json.dumps(settings, indent=2) + '\n')
for name in ('settings.json', '.claude.json'):
    (root / 'prepared' / name).write_bytes((profile / name).read_bytes())
(root / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
(root / 'control.json').write_text(json.dumps({'head': None}))
plugin = root / 'plugin'
(plugin / '.claude-plugin').mkdir()
(plugin / 'hooks').mkdir()
(plugin / '.claude-plugin/plugin.json').write_text(json.dumps({
    'name': 'agentchat-body-boundary-probe', 'version': '0.1.0',
    'description': 'Disposable full-body hook observation, not product admission',
}))
module_path = args.module.resolve(strict=True) if args.module else source / 'observe.js'
module = module_path.read_text()
manifest['module_source'] = str(module_path)
manifest['module_sha256'] = hashlib.sha256(module.encode()).hexdigest()
(root / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
for token, value in [('__ROOT__', str(root)), ('__PYTHON__', sys.executable), ('__EXPECTED__', session)]:
    module = module.replace(token, json.dumps(value))
(plugin / 'hooks/observe.js').write_text(module)
(plugin / 'hooks/hooks.json').write_text(json.dumps({
    'modules': ['./observe.js'],
}, indent=2))
(root / 'workspace/.agentchat-body-probe-wake').write_text('0\n')
print(json.dumps({'root': str(root), 'session': session, 'profile': str(profile)}))

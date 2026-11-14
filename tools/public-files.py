#!/usr/bin/env python3
"""List intended public files without changing Git, local state or publishing."""

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import subprocess

ROOT_FILES = {
    '.gitignore', 'AGENTS.md', 'CLAUDE.md', 'README.md', 'Cargo.toml', 'Cargo.lock',
    'Makefile', 'rust-toolchain.toml', 'deny.toml', 'chigiri.toml',
    'LICENSE-MIT', 'LICENSE-APACHE',
    'craftguild.toml',
}
PUBLIC_TREES = ('crates/', 'generated/', 'tools/', 'docs/', 'web/', '.github/',
                'standards/', 'scripts/', '.agents/skills/')
PRIVATE_TREES = ('.git/', '.private/', '.beads/', '.claude/', '.codex/', '.cursor/',
                 '.craftguild/',
                 '.agentchat/', '.ac/',
                 '.quality-tools/', 'target/', 'web/node_modules/', 'web/dist/',
                 'web/test-results/', 'web/playwright-report/',
                 'experiments/web-api-poc/')
PRIVATE_FILES = {'.ac-wakeup', '.agentchat-wakeup', '.beads.gate.lock',
                 '.beads-credential-key', 'web/viewer-context.json',
                 'web/public/build-info.txt'}


def classification(name):
    """Explicit roots keep new workstation files from silently becoming public."""
    path = PurePosixPath(name)
    if path.is_absolute() or '..' in path.parts or str(path) != name:
        raise ValueError('Invalid inventory path: ' + name)
    if (name in PRIVATE_FILES or name.startswith(PRIVATE_TREES)
            or any(p in {'.DS_Store', '__pycache__', '.chigiri-cache'} for p in path.parts)
            or path.suffix == '.pyc'):
        return 'private'
    if name in ROOT_FILES or name.startswith(PUBLIC_TREES):
        return 'public'
    return 'unclassified'


def inventory(root):
    """Git selects existing source candidates; ignored cached private files still filter."""
    routing = {'GIT_DIR', 'GIT_WORK_TREE', 'GIT_COMMON_DIR', 'GIT_INDEX_FILE',
               'GIT_OBJECT_DIRECTORY', 'GIT_ALTERNATE_OBJECT_DIRECTORIES'}
    env = {key: value for key, value in os.environ.items() if key not in routing}
    result = subprocess.run(['git', 'ls-files', '--cached', '--others',
                             '--exclude-standard', '-z'], cwd=root, check=True,
                            capture_output=True, env=env)
    names = sorted({name.decode('utf-8') for name in result.stdout.split(b'\0') if name})
    files, unclassified = [], []
    for name in names:
        kind = classification(name)
        if kind == 'private':
            continue
        path = root / name
        if path.is_symlink():
            raise ValueError('Public inventory refuses symlinks: ' + name)
        if not path.exists():
            continue  # Cached unstaged deletions are not public source files.
        if kind == 'unclassified':
            unclassified.append(name)
        elif path.is_file():
            data = path.read_bytes()
            files.append({'path': name, 'bytes': len(data),
                          'sha256': hashlib.sha256(data).hexdigest()})
        else:
            raise ValueError('Nonregular public candidate: ' + name)
    if unclassified:
        raise ValueError('Classify new root paths before export: ' + ', '.join(unclassified))
    return {'schema': 'agentchat/public-files-v1', 'files': files}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.parse_args()
    print(json.dumps(inventory(Path(__file__).resolve().parent.parent), indent=2))


if __name__ == '__main__':
    main()

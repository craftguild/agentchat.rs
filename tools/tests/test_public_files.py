"""Exercise actual Git selection and public/private export boundaries."""
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('public_files',
    Path(__file__).resolve().parents[1] / 'public-files.py')
PUBLIC = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PUBLIC)


class PublicFilesTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='agentchat-public-files-')
        self.root = Path(self.temp.name)
        env = {key: value for key, value in os.environ.items()
               if not key.startswith('GIT_')}
        env.update(GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL=os.devnull,
                   GIT_TERMINAL_PROMPT='0')
        self.environment = patch.dict(os.environ, env, clear=True)
        self.environment.start()
        self.addCleanup(self.environment.stop)
        subprocess.run(['git', 'init', '--quiet', str(self.root)], check=True)

    def tearDown(self):
        self.temp.cleanup()

    def put(self, name, data=b'fixture'):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        return path

    def names(self):
        return [x['path'] for x in PUBLIC.inventory(self.root)['files']]

    def test_actual_cached_private_metadata_and_ignored_archive_are_excluded(self):
        self.put('.gitignore', b'/.private/\n')
        self.put('README.md', b'public')
        self.put('.beads/config.yaml', b'private')
        self.put('.codex/config.toml', b'private')
        self.put('.private/oss-archive/raw.md', b'private')
        subprocess.run(['git', 'add', '-f', '.beads/config.yaml', '.codex/config.toml',
                        '.private/oss-archive/raw.md'], cwd=self.root, check=True)
        self.assertEqual(self.names(), ['.gitignore', 'README.md'])
        self.assertTrue((self.root / '.beads/config.yaml').is_file())
        self.assertTrue((self.root / '.private/oss-archive/raw.md').is_file())

    def test_removed_cached_source_is_absent_and_untracked_successor_is_present(self):
        old = self.put('docs/old.md')
        subprocess.run(['git', 'add', 'docs/old.md'], cwd=self.root, check=True)
        old.unlink()
        self.put('docs/guides/new.md')
        self.assertEqual(self.names(), ['docs/guides/new.md'])

    def test_cache_and_independent_workspace_do_not_enter_public_inventory(self):
        for name in ['generated/.chigiri-cache/probe.rs', 'tools/__pycache__/test.pyc',
                     'web/node_modules/pkg/index.js', 'experiments/web-api-poc/private.md',
                     '.claude/settings.json', '.cursor/hooks.json', '.ac-wakeup']:
            self.put(name)
        self.put('generated/contract/src/lib.rs')
        self.assertEqual(self.names(), ['generated/contract/src/lib.rs'])

    def test_unknown_root_and_symlink_refuse_instead_of_leaking(self):
        unknown = self.put('private-notes.txt')
        with self.assertRaisesRegex(ValueError, 'Classify new root paths'):
            self.names()
        unknown.unlink()
        self.put('README.md')
        (self.root / 'docs').mkdir()
        (self.root / 'docs/link.md').symlink_to(self.root / 'README.md')
        with self.assertRaisesRegex(ValueError, 'refuses symlinks'):
            self.names()

    def test_unicode_paths_exact_bytes_and_known_project_skill_are_retained(self):
        self.put('docs/日本語.md', b'\0\r\nexact')
        self.put('.agents/skills/beads/SKILL.md', b'portable')
        rows = PUBLIC.inventory(self.root)['files']
        self.assertEqual([x['path'] for x in rows],
                         ['.agents/skills/beads/SKILL.md', 'docs/日本語.md'])
        self.assertEqual(rows[-1]['bytes'], 8)
        self.assertEqual(len(rows[-1]['sha256']), 64)

    def test_inherited_git_routing_cannot_select_another_repository(self):
        self.put('README.md', b'owned')
        with tempfile.TemporaryDirectory(prefix='agentchat-foreign-git-') as other:
            subprocess.run(['git', 'init', '--quiet', other], check=True)
            (Path(other) / 'Cargo.toml').write_text('foreign')
            with patch.dict(os.environ, {'GIT_DIR': str(Path(other) / '.git'),
                                        'GIT_WORK_TREE': other}):
                self.assertEqual(self.names(), ['README.md'])


if __name__ == '__main__':
    unittest.main()
